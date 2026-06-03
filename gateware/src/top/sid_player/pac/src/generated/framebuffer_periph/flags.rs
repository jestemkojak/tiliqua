#[doc = "Register `flags` reader"]
pub type R = crate::R<FLAGS_SPEC>;
#[doc = "Register `flags` writer"]
pub type W = crate::W<FLAGS_SPEC>;
#[doc = "Field `enable` writer - enable field"]
pub type ENABLE_W<'a, REG> = crate::BitWriter<'a, REG>;
#[doc = "Field `rotation` writer - rotation field"]
pub type ROTATION_W<'a, REG> = crate::FieldWriter<'a, REG, 2>;
impl W {
    #[doc = "Bit 0 - enable field"]
    #[inline(always)]
    pub fn enable(&mut self) -> ENABLE_W<'_, FLAGS_SPEC> {
        ENABLE_W::new(self, 0)
    }
    #[doc = "Bits 1:2 - rotation field"]
    #[inline(always)]
    pub fn rotation(&mut self) -> ROTATION_W<'_, FLAGS_SPEC> {
        ROTATION_W::new(self, 1)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`flags::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`flags::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct FLAGS_SPEC;
impl crate::RegisterSpec for FLAGS_SPEC {
    type Ux = u8;
}
#[doc = "`read()` method returns [`flags::R`](R) reader structure"]
impl crate::Readable for FLAGS_SPEC {}
#[doc = "`write(|w| ..)` method takes [`flags::W`](W) writer structure"]
impl crate::Writable for FLAGS_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets flags to value 0"]
impl crate::Resettable for FLAGS_SPEC {}
